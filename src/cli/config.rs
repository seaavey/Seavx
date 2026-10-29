//! Effective configuration reporting and atomic config writes.
//!
//! Shared by `seavx`, the TUI `/config` command and the model picker so every
//! path validates and persists values the same way.

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

/// Strip userinfo, query and fragment from a URL before printing it. The marker
/// says something was removed, so a reader never mistakes the shorter URL for
/// the value that was stored.
pub fn redact_url(raw: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(raw) {
        let redacted =
            !url.username().is_empty() || url.password().is_some() || url.query().is_some();
        let _ = url.set_password(None);
        let _ = url.set_username("");
        url.set_query(None);

        let mut text = url.to_string();
        if redacted {
            text.push_str(" (userinfo/query redacted)");
        }
        return text;
    }

    let without_fragment = raw.split('#').next().unwrap_or(raw);
    let without_query = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    let stripped = match (without_query.find("://"), without_query.rfind('@')) {
        (Some(scheme), Some(at)) if at > scheme + 3 => {
            format!(
                "{}://{}",
                &without_query[..scheme],
                &without_query[at + 1..]
            )
        }
        _ => without_query.to_string(),
    };
    if stripped != raw {
        format!("{stripped} (userinfo/query redacted)")
    } else {
        stripped
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
                // Never repeat the input: it may carry userinfo or a token.
                _ => Err("invalid base_url: expected a URL with a host".to_string()),
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

/// How an existing file is treated when it does not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLoad {
    /// Every targeted mutation: an unparsable file is an error.
    Strict,
    /// Only the two surfaces that legitimately rewrite a whole file
    /// (`seavx setup providers`, `seavx model <name>`): the malformed bytes are
    /// preserved next to the config and the document starts from the seed.
    RepairUnparsable,
}

/// Write one key, preserving comments, ordering and unknown keys.
pub fn set_value(path: &Path, key: ConfigKey, raw: &str) -> Result<String, String> {
    let value = canonical_value(key, raw)?;
    update_config(path, |document| {
        if key == ConfigKey::MaxTurns {
            let turns: i64 = value
                .parse()
                .map_err(|_| "max_turns must be an integer".to_string())?;
            document[key.name()] = toml_edit::value(turns);
        } else {
            document[key.name()] = toml_edit::value(value.clone());
        }
        Ok(())
    })?;

    // A URL is echoed only with its userinfo and query removed.
    if key == ConfigKey::BaseUrl {
        return Ok(redact_url(&value));
    }
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

    // An absent key is not a write: peeking avoids rewriting bytes, mode or a backup.
    if load_document(path)?.get(key.name()).is_none() {
        return Ok(false);
    }
    update_config(path, |document| {
        document.remove(key.name());
        Ok(())
    })?;
    Ok(true)
}

/// Persist the whole [`AgentConfig`], never `api_key`: `AgentConfig::load` may
/// carry a value from the environment, and a model change must not move a secret
/// onto disk. Only [`set_api_key`] writes that field, for a key the user typed.
pub fn write_agent_config(path: &Path, config: &AgentConfig) -> Result<(), String> {
    update_config(path, |document| {
        write_fields(document, config);
        Ok(())
    })
}

/// [`write_agent_config`] with the repair mode `seavx setup`/`seavx model` need.
pub fn write_agent_config_repairing(
    path: &Path,
    config: &AgentConfig,
) -> Result<Option<PathBuf>, String> {
    update_config_with(path, ConfigLoad::RepairUnparsable, |document| {
        write_fields(document, config);
        Ok(())
    })
}

fn write_fields(document: &mut toml_edit::DocumentMut, config: &AgentConfig) {
    document["model"] = toml_edit::value(config.model.clone());
    document["max_turns"] = toml_edit::value(config.max_turns as i64);
    set_or_remove(document, "base_url", config.base_url.as_deref());
    set_or_remove(
        document,
        "reasoning_effort",
        config.reasoning_effort.as_deref(),
    );
    set_or_remove(document, "system_prompt", config.system_prompt.as_deref());
}

/// The only writer of `api_key`: a value the user entered in `seavx setup`.
pub fn set_api_key(path: &Path, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("api_key must not be empty".to_string());
    }
    let key = key.to_string();
    update_config(path, |document| {
        document["api_key"] = toml_edit::value(key.clone());
        Ok(())
    })
}

/// [`set_model_and_effort`] with the repair mode `seavx model` needs.
pub fn set_model_and_effort_repairing(
    path: &Path,
    model: &str,
    effort: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let model = canonical_value(ConfigKey::Model, model)?;
    let effort = match effort {
        Some(raw) => Some(canonical_value(ConfigKey::ReasoningEffort, raw)?),
        None => None,
    };
    update_config_with(path, ConfigLoad::RepairUnparsable, |document| {
        document["model"] = toml_edit::value(model.clone());
        if let Some(effort) = &effort {
            document["reasoning_effort"] = toml_edit::value(effort.clone());
        }
        Ok(())
    })
}

/// Set the model and, optionally, the reasoning effort as one write.
pub fn set_model_and_effort(path: &Path, model: &str, effort: Option<&str>) -> Result<(), String> {
    let model = canonical_value(ConfigKey::Model, model)?;
    let effort = match effort {
        Some(raw) => Some(canonical_value(ConfigKey::ReasoningEffort, raw)?),
        None => None,
    };

    update_config(path, |document| {
        document["model"] = toml_edit::value(model.clone());
        if let Some(effort) = &effort {
            document["reasoning_effort"] = toml_edit::value(effort.clone());
        }
        Ok(())
    })
}

fn set_or_remove(document: &mut toml_edit::DocumentMut, key: &str, value: Option<&str>) {
    match value {
        Some(value) => document[key] = toml_edit::value(value),
        None => {
            document.remove(key);
        }
    }
}

fn load_document(path: &Path) -> Result<toml_edit::DocumentMut, String> {
    load_document_with(path, ConfigLoad::Strict).map(|(document, _)| document)
}

fn load_document_with(
    path: &Path,
    load: ConfigLoad,
) -> Result<(toml_edit::DocumentMut, Option<PathBuf>), String> {
    // A crashed replace leaves a backup behind; resolve it before reading.
    recover_config(path)?;

    if !path.exists() {
        return Ok((seed_document(), None));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    match text.parse::<toml_edit::DocumentMut>() {
        Ok(document) => Ok((document, None)),
        Err(error) => {
            let message = format!(
                "failed to parse {}: {}",
                path.display(),
                first_line(&error.to_string())
            );
            match load {
                ConfigLoad::Strict => Err(message),
                ConfigLoad::RepairUnparsable => {
                    let preserved = preserve_unparsable(path)?;
                    Ok((seed_document(), Some(preserved)))
                }
            }
        }
    }
}

/// `model` and `max_turns` are required by `AgentConfig`, and a file replaces
/// the built-in defaults wholesale; seeding them keeps the result loadable.
fn seed_document() -> toml_edit::DocumentMut {
    let mut document = toml_edit::DocumentMut::new();
    document["model"] = toml_edit::value(DEFAULT_MODEL);
    document["max_turns"] = toml_edit::value(DEFAULT_MAX_TURNS);
    document
}

/// Keep the bytes of a file that could not be parsed, beside it and owner-only,
/// so a repair never destroys what the user wrote.
fn preserve_unparsable(path: &Path) -> Result<PathBuf, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.toml".to_string());
    let preserved = path.with_file_name(format!("{name}.invalid-{}", crate::util::epoch_millis()));
    crate::atomic_file::write_atomic(
        &preserved,
        &bytes,
        crate::atomic_file::ModePolicy::Force(0o600),
    )
    .map_err(|error| format!("failed to preserve {}: {error}", preserved.display()))?;
    Ok(preserved)
}

/// The one config writer: load, edit, validate the complete document, then
/// replace the file once. In-process callers are serialised so two of them
/// cannot interleave a read and a write. A second *process* can still land in
/// the window between the final re-check and the rename; that case ends in the
/// `config recovery conflict` branch instead of losing data.
pub fn update_config(
    path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>,
) -> Result<(), String> {
    update_config_with(path, ConfigLoad::Strict, edit).map(|_| ())
}

/// [`update_config`] with an explicit load mode. Returns the preserved copy when
/// a repair replaced an unparsable file, so the caller can name it once.
pub fn update_config_with(
    path: &Path,
    load: ConfigLoad,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>,
) -> Result<Option<PathBuf>, String> {
    static CONFIG_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = CONFIG_WRITE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let (mut document, preserved) = load_document_with(path, load)?;
    edit(&mut document)?;

    let text = document.to_string();
    // Validate the whole resulting document before the file is touched.
    text.parse::<toml::Value>().map_err(|error| {
        format!(
            "failed to validate {}: {}",
            path.display(),
            first_line(&error.to_string())
        )
    })?;

    replace_config(path, text.as_bytes())?;
    Ok(preserved)
}

/// Parser errors echo the offending source lines; only the first line is safe
/// to show because the source may be an `api_key`.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

const BAK_MARKER: &str = ".bak-";

/// Crash points a test can force. Production passes `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Creating the `.bak` copy fails.
    BackupCreate,
    /// Another writer replaces the target between capture and the re-check.
    ChangeTargetDuringWrite,
    /// Removing the target for the replacement fails.
    TargetRemove,
    /// A second writer recreates the target between the removal and the rename.
    ThirdStateAfterRemove,
    /// The rename into place fails.
    Rename,
    /// A second writer lands between the rename and the verification.
    ThirdStateAfterRename,
    /// Removing the `.bak` after a verified write fails.
    BackupCleanup,
}

struct Backup {
    path: PathBuf,
    record: PathBuf,
}

impl Backup {
    fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(&self.record);
    }
}

/// Capture the target, keep a private `.bak` copy, replace the target, then
/// verify the result. Never leaves the process without a usable config.
fn replace_config(path: &Path, contents: &[u8]) -> Result<(), String> {
    replace_config_with(path, contents, None)
}

fn replace_config_with(path: &Path, contents: &[u8], fault: Option<Fault>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }

    // (1) Capture before anything is written.
    let captured = std::fs::read(path).ok();

    // (2) Stage the new bytes.
    let temp = crate::atomic_file::unique_temp_path(path);
    if let Err(error) = stage(&temp, contents) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }

    // (3) Keep a private copy of what is being replaced.
    let backup = match captured.as_deref() {
        Some(previous) => {
            if fault == Some(Fault::BackupCreate) {
                let _ = std::fs::remove_file(&temp);
                return Err(format!("failed to create a backup of {}", path.display()));
            }
            match write_backup(path, previous, contents) {
                Ok(backup) => Some(backup),
                Err(error) => {
                    let _ = std::fs::remove_file(&temp);
                    return Err(error);
                }
            }
        }
        None => None,
    };

    let abort = |message: String| -> Result<(), String> {
        let _ = std::fs::remove_file(&temp);
        if let Some(backup) = &backup {
            backup.remove();
        }
        Err(message)
    };

    // (4) Abort when the target moved under us since the capture.
    if fault == Some(Fault::ChangeTargetDuringWrite)
        && let Err(error) = std::fs::write(path, b"concurrent = true\n")
    {
        return abort(format!("failed to write {}: {error}", path.display()));
    }
    if std::fs::read(path).ok() != captured {
        return abort("config changed during write".to_string());
    }
    // Re-check immediately before the removal: the last moment the captured
    // bytes can still be the ones on disk.
    if std::fs::read(path).ok() != captured {
        return abort("config changed during write".to_string());
    }

    // (5) Remove the target.
    if fault == Some(Fault::TargetRemove) {
        return abort(format!("failed to remove {}", path.display()));
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return abort(format!("failed to remove {}: {error}", path.display())),
    }

    if fault == Some(Fault::ThirdStateAfterRemove)
        && let Err(error) = std::fs::write(path, b"third = 3\n")
    {
        return abort(format!("failed to write {}: {error}", path.display()));
    }
    if path.exists() {
        // Someone else wrote a third state. Leave it alone and keep the backup
        // for the user to decide instead of overwriting their file.
        let _ = std::fs::remove_file(&temp);
        let kept = backup
            .as_ref()
            .map(|backup| backup.path.display().to_string())
            .unwrap_or_else(|| "none".to_string());
        return Err(format!(
            "config recovery conflict: {}; backup kept at {kept}",
            path.display()
        ));
    }

    // (6) Rename into place.
    let replaced = fault != Some(Fault::Rename) && std::fs::rename(&temp, path).is_ok();
    if !replaced {
        restore_previous(path, captured.as_deref());
        return abort(format!("failed to replace {}", path.display()));
    }

    if fault == Some(Fault::ThirdStateAfterRename)
        && let Err(error) = std::fs::write(path, b"third = 3\n")
    {
        return abort(format!("failed to write {}: {error}", path.display()));
    }

    // (7) Verify the new bytes, then drop the backup. A mismatch means another
    // writer landed after the rename, so their file is kept: restoring our old
    // bytes here would delete work this code never saw.
    if std::fs::read(path).ok().as_deref() != Some(contents) {
        let kept = backup
            .as_ref()
            .map(|backup| backup.path.display().to_string())
            .unwrap_or_else(|| "none".to_string());
        return Err(format!(
            "config recovery conflict: {}; backup kept at {kept}",
            path.display()
        ));
    }
    if fault != Some(Fault::BackupCleanup)
        && let Some(backup) = &backup
    {
        backup.remove();
    }
    Ok(())
}

fn stage(temp: &Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = create_exclusive(temp)?;
    file.write_all(contents)
        .map_err(|error| format!("failed to write {}: {error}", temp.display()))?;
    file.sync_all()
        .map_err(|error| format!("failed to flush {}: {error}", temp.display()))
}

fn create_exclusive(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))
}

/// The backup carries the bytes being replaced; the `.new` record names the
/// bytes the interrupted write intended, so recovery can tell the new state from
/// a third one.
fn write_backup(path: &Path, previous: &[u8], next: &[u8]) -> Result<Backup, String> {
    use std::io::Write;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "config.toml".to_string());
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = crate::util::epoch_millis();
    let counter = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The trailing length is what recovery checks the bytes against.
    let backup_path = path.with_file_name(format!(
        "{name}{BAK_MARKER}{stamp}-{counter}-{}",
        previous.len()
    ));

    let mut file = create_exclusive(&backup_path)?;
    file.write_all(previous)
        .map_err(|error| format!("failed to write {}: {error}", backup_path.display()))?;
    file.sync_all()
        .map_err(|error| format!("failed to flush {}: {error}", backup_path.display()))?;
    drop(file);

    let record = PathBuf::from(format!("{}.new", backup_path.display()));
    let contents = format!("{}\t{}\n", next.len(), crate::checkpoint::fnv1a64(next));
    if let Err(error) = std::fs::write(&record, contents) {
        let _ = std::fs::remove_file(&backup_path);
        return Err(format!("failed to record {}: {error}", record.display()));
    }

    Ok(Backup {
        path: backup_path,
        record,
    })
}

fn restore_previous(path: &Path, previous: Option<&[u8]>) {
    if let Some(previous) = previous {
        let _ = crate::atomic_file::write_atomic(
            path,
            previous,
            crate::atomic_file::ModePolicy::Force(0o600),
        );
    }
}

/// Resolve a backup left by a crashed replace. Never deletes the only good copy.
pub fn recover_config(path: &Path) -> Result<(), String> {
    let Some(backup_path) = newest_backup(path) else {
        return Ok(());
    };
    let record = PathBuf::from(format!("{}.new", backup_path.display()));
    let intended = std::fs::read_to_string(&record)
        .ok()
        .and_then(|text| parse_record(&text));
    let previous = std::fs::read(&backup_path).ok();
    let current = std::fs::read(path).ok();

    let discard = || {
        prune_backups(path, Some(&backup_path));
        let _ = std::fs::remove_file(&backup_path);
        let _ = std::fs::remove_file(&record);
    };

    match (current, previous) {
        (None, None) => Err(format!("config unavailable: {}", path.display())),
        (None, Some(previous)) => {
            let usable = std::fs::symlink_metadata(&backup_path)
                .map(|metadata| {
                    metadata.is_file() && backup_length(&backup_path) == Some(previous.len() as u64)
                })
                .unwrap_or(false);
            if !usable {
                return Err(format!(
                    "config unavailable: {}; backup kept at {}",
                    path.display(),
                    backup_path.display()
                ));
            }
            crate::atomic_file::write_atomic(
                path,
                &previous,
                crate::atomic_file::ModePolicy::Force(0o600),
            )
            .map_err(|error| format!("failed to restore {}: {error}", path.display()))?;
            discard();
            Ok(())
        }
        (Some(current), Some(previous)) => {
            let landed = intended.is_some_and(|(len, sum)| {
                current.len() as u64 == len && crate::checkpoint::fnv1a64(&current) == sum
            });
            if landed || current == previous {
                discard();
                return Ok(());
            }
            Err(format!(
                "config recovery conflict: {}; backup kept at {}",
                path.display(),
                backup_path.display()
            ))
        }
        // An unreadable backup cannot be used; leave it for the user.
        (Some(_), None) => Ok(()),
    }
}

fn parse_record(text: &str) -> Option<(u64, u64)> {
    let (len, sum) = text.trim().split_once('\t')?;
    Some((len.parse().ok()?, sum.parse().ok()?))
}

fn backup_length(backup: &Path) -> Option<u64> {
    let name = backup.file_name()?.to_str()?;
    let (_, length) = name.rsplit_once('-')?;
    length.parse().ok()
}

fn all_backups(path: &Path) -> Vec<PathBuf> {
    let Some(name) = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
    else {
        return Vec::new();
    };
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let prefix = format!("{name}{BAK_MARKER}");
    let mut backups: Vec<PathBuf> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate
                .file_name()
                // The `.new` record shares the prefix but is not a backup.
                .map(|file| {
                    let file = file.to_string_lossy();
                    file.starts_with(&prefix) && !file.ends_with(".new")
                })
                .unwrap_or(false)
        })
        .collect();
    backups.sort();
    backups
}

fn newest_backup(path: &Path) -> Option<PathBuf> {
    all_backups(path).pop()
}

/// A resolved replace leaves nothing behind: an older backup can only be a
/// leftover from an earlier crash, and its record goes with it.
fn prune_backups(path: &Path, keep: Option<&Path>) {
    for backup in all_backups(path) {
        if Some(backup.as_path()) == keep {
            continue;
        }
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::remove_file(PathBuf::from(format!("{}.new", backup.display())));
    }
}

/// Create `.seavx/config.toml` when missing. Returns `(path, created)`.
pub fn init_workspace(workspace_root: &Path) -> Result<(PathBuf, bool), String> {
    let path = workspace_config_path(workspace_root);
    if path.exists() {
        return Ok((path, false));
    }

    update_config(&path, |document| {
        document["model"] = toml_edit::value(DEFAULT_MODEL);
        document["max_turns"] = toml_edit::value(DEFAULT_MAX_TURNS);
        document["base_url"] = toml_edit::value(DEFAULT_BASE_URL);
        document["api_key"] = toml_edit::value("");
        Ok(())
    })?;
    Ok((path, true))
}

#[cfg(test)]
#[path = "../tests/cli/config.rs"]
mod tests;
