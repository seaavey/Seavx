//! Environment and configuration health checks shared by `seavx doctor` and
//! the TUI `/doctor` command.
//!
//! The network probe is injected so tests never perform HTTP.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::agent::engine::{
    AgentConfig, ConfigOrigin, ConfigReport, DEFAULT_BASE_URL, model_context_limit,
    normalize_base_url,
};
use crate::agent::models::{ModelCache, ModelManager};
use crate::agent::skills::SkillManager;
use crate::cli::config::{mask_key, redact_url};
use crate::cli::memory::{project_candidates, user_candidates};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub id: &'static str,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct DoctorReport {
    pub version: String,
    pub platform: String,
    pub checks: Vec<CheckResult>,
}

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub offline: bool,
    pub workspace_root: PathBuf,
    pub home: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeResult {
    Ok { models: usize },
    Unauthorized,
    NotFound,
    Unreachable(String),
    Status(u16),
}

#[async_trait::async_trait]
pub trait EndpointProbe: Send + Sync {
    async fn probe(&self, base_url: &str, api_key: Option<&str>) -> ProbeResult;
}

pub async fn run(options: DoctorOptions) -> DoctorReport {
    run_with_probe(options, &HttpProbe).await
}

pub async fn run_with_probe(options: DoctorOptions, probe: &dyn EndpointProbe) -> DoctorReport {
    let (config, config_report) = AgentConfig::load_with_report_home(
        &options.workspace_root,
        options.home.as_deref(),
        |name| std::env::var(name).ok(),
    );
    let (base_url, _) = normalize_base_url(config.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL));

    let checks = vec![
        binary_check(),
        config_check(&config_report),
        config_recovery_check(&config_report),
        secret_check(&config),
        context_check(&config),
        permissions_check(&config),
        stream_check(&config, options.offline),
        endpoint_check(&options, &config, &base_url, probe).await,
        cache_check(&options.workspace_root, &base_url),
        workspace_check(&options.workspace_root),
        session_check(&options),
        memory_check(&options),
        skills_check(&options.workspace_root),
    ];

    DoctorReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
        checks,
    }
}

fn binary_check() -> CheckResult {
    ok(
        "binary",
        format!(
            "v{} ({}/{})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    )
}

fn config_check(report: &ConfigReport) -> CheckResult {
    if let Some(error) = &report.parse_error {
        let path = report
            .active_path
            .as_deref()
            .unwrap_or(&report.workspace_path);
        return fail(
            "config",
            format!("{} (parse error: {error})", display_path(path)),
        );
    }

    match &report.active_path {
        Some(path) => ok(
            "config",
            format!(
                "{} ({})",
                display_path(path),
                origin_label(report.active_origin)
            ),
        ),
        None => ok("config", "no file; using defaults"),
    }
}

/// The limit the budget uses, and whether it is this build's conservative guess.
fn context_check(config: &AgentConfig) -> CheckResult {
    let limit = model_context_limit(&config.model);
    let source = if limit.assumed {
        "assumed".to_string()
    } else {
        config.model.clone()
    };
    ok("context", format!("{} tokens ({source})", limit.tokens))
}

fn secret_check(config: &AgentConfig) -> CheckResult {
    match config.api_key.as_deref().filter(|key| !key.is_empty()) {
        Some(key) => ok("secret", mask_key(key)),
        None => warn("secret", "no API key; run `seavx setup providers`"),
    }
}

async fn endpoint_check(
    options: &DoctorOptions,
    config: &AgentConfig,
    base_url: &str,
    probe: &dyn EndpointProbe,
) -> CheckResult {
    if options.offline {
        return CheckResult {
            id: "endpoint",
            status: CheckStatus::Skip,
            detail: "offline".to_string(),
        };
    }

    let safe_url = redact_url(base_url);
    let api_key = config.api_key.as_deref().filter(|key| !key.is_empty());
    match probe.probe(base_url, api_key).await {
        ProbeResult::Ok { models } => ok("endpoint", format!("{safe_url} ({models} models)")),
        ProbeResult::Unauthorized => fail("endpoint", format!("{safe_url} (unauthorized)")),
        ProbeResult::NotFound => warn(
            "endpoint",
            format!("{safe_url} (models endpoint not found)"),
        ),
        ProbeResult::Unreachable(error) => warn(
            "endpoint",
            format!("{safe_url} ({})", redact_error(&error, base_url)),
        ),
        ProbeResult::Status(code) => warn("endpoint", format!("{safe_url} (HTTP {code})")),
    }
}

fn cache_check(workspace_root: &Path, base_url: &str) -> CheckResult {
    let slug = ModelManager::domain_slug(base_url);
    let path = workspace_root.join(format!(".seavx/cache/models-{slug}.json"));

    if !path.exists() {
        return warn("cache", "missing; run `seavx model` to populate");
    }

    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<ModelCache>(&text) {
            Ok(cache) if cache.models.is_empty() => {
                warn("cache", "empty; run `seavx model` to populate")
            }
            Ok(cache) => ok(
                "cache",
                format!("{} models ({})", cache.models.len(), display_path(&path)),
            ),
            Err(error) => warn("cache", format!("unreadable: {error}")),
        },
        Err(error) => warn("cache", format!("unreadable: {error}")),
    }
}

fn workspace_check(workspace_root: &Path) -> CheckResult {
    let directory = workspace_root.join(".seavx");
    let existed = directory.exists();

    if let Err(error) = std::fs::create_dir_all(&directory) {
        return fail(
            "workspace",
            format!("cannot create {}: {error}", display_path(&directory)),
        );
    }

    let probe = match create_probe(&directory, &probe_name()) {
        Ok(probe) => probe,
        Err(ProbeError::InUse) => return warn("workspace", "probe name in use; not overwritten"),
        Err(ProbeError::Failed(detail)) => {
            return fail("workspace", format!("not writable: {detail}"));
        }
    };
    if let Err(error) = remove_probe(probe) {
        return fail("workspace", error);
    }

    if existed {
        ok("workspace", "writable")
    } else {
        warn("workspace", "created `.seavx` during check")
    }
}

/// A name no other run can hold: this process, this instant.
pub(crate) fn probe_name() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!(".{}-{nanos}", std::process::id())
}

/// A probe file this run created, identified by the filesystem's own handle.
#[derive(Debug)]
pub(crate) struct Probe {
    path: PathBuf,
    id: FileId,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FileId(u64, u64);

fn file_id(metadata: &std::fs::Metadata) -> FileId {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileId(metadata.dev(), metadata.ino())
    }
    #[cfg(not(unix))]
    {
        FileId(metadata.len(), 0)
    }
}

#[derive(Debug)]
pub(crate) enum ProbeError {
    /// Something is already at that name: never followed, never overwritten.
    InUse,
    Failed(String),
}

/// Create the probe exclusively and owner-only. `create_new` is what makes a
/// planted symlink or file a `WARN` instead of a write through it.
pub(crate) fn create_probe(directory: &Path, name: &str) -> Result<Probe, ProbeError> {
    let path = directory.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(ProbeError::InUse);
        }
        Err(error) => return Err(ProbeError::Failed(error.to_string())),
    };
    let id = file
        .metadata()
        .map(|metadata| file_id(&metadata))
        .map_err(|error| ProbeError::Failed(error.to_string()))?;
    use std::io::Write;
    file.write_all(b"ok")
        .map_err(|error| ProbeError::Failed(error.to_string()))?;
    drop(file);

    Ok(Probe { path, id })
}

/// Remove exactly the file this run created: a same-named replacement is left
/// alone, and a failed cleanup is reported instead of hidden.
pub(crate) fn remove_probe(probe: Probe) -> Result<(), String> {
    let current = std::fs::metadata(&probe.path)
        .map(|metadata| file_id(&metadata))
        .map_err(|error| format!("probe cleanup failed: {error}"))?;
    if current != probe.id {
        return Err(format!(
            "probe cleanup refused: {} is no longer the file this check created",
            display_path(&probe.path)
        ));
    }
    std::fs::remove_file(&probe.path).map_err(|error| format!("probe cleanup failed: {error}"))
}

/// A `.bak` the loader could not resolve: name it instead of leaving it in a
/// `tracing::warn!`. Load already succeeded with whatever file is readable.
fn config_recovery_check(report: &ConfigReport) -> CheckResult {
    let path = report
        .active_path
        .as_deref()
        .unwrap_or(&report.workspace_path);
    match crate::cli::config::recover_config(path) {
        Ok(()) => ok("config-recovery", "no pending backup conflict"),
        Err(conflict) => warn("config-recovery", conflict),
    }
}

/// The permission mode and the categories lifted out of it; never a secret.
fn permissions_check(config: &AgentConfig) -> CheckResult {
    let persisted = if config.permitted_actions.is_empty() {
        "none".to_string()
    } else {
        config.permitted_actions.join(" ")
    };
    match config.tool_permissions.as_str() {
        "deny" | "ask" | "allow" => ok(
            "permissions",
            format!("{} (persisted: {persisted})", config.tool_permissions),
        ),
        other => fail("permissions", format!("invalid tool_permissions: {other}")),
    }
}

/// A capability probe would spend a provider request; this reports the setting
/// and what a failure falls back to, unless `--offline` hides the endpoint.
fn stream_check(config: &AgentConfig, offline: bool) -> CheckResult {
    let suffix = if offline { " (not probed)" } else { "" };
    if config.stream {
        ok(
            "stream",
            format!("enabled{suffix}; a failed stream falls back to one plain request"),
        )
    } else {
        ok(
            "stream",
            format!("disabled{suffix}; every turn sends one plain request"),
        )
    }
}

/// Where this run's session database lives, derived from the report's paths so a
/// test never opens the checkout's own file.
fn session_path(options: &DoctorOptions) -> Option<PathBuf> {
    if crate::util::is_debug() {
        return Some(options.workspace_root.join(".seavx").join("sessions.db"));
    }
    options
        .home
        .as_ref()
        .map(|home| home.join(".seavx").join("sessions.db"))
}

/// Path, schema version, payload format and writability, read without writing.
fn session_check(options: &DoctorOptions) -> CheckResult {
    let Some(path) = session_path(options) else {
        return warn("session", "home directory is not set");
    };
    let directory = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let writable = std::fs::create_dir_all(&directory).is_ok()
        && create_probe(&directory, &probe_name())
            .ok()
            .is_some_and(|probe| remove_probe(probe).is_ok());

    if !path.exists() {
        return ok(
            "session",
            format!(
                "{} (empty; created with the first prompt)",
                display_path(&path)
            ),
        );
    }

    let header =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|error| error.to_string())
            .and_then(|connection| {
                let version: i64 = connection
                    .query_row("PRAGMA user_version", [], |row| row.get(0))
                    .map_err(|error| error.to_string())?;
                let format: String = connection
                    .query_row(
                        "SELECT payload_format FROM sessions ORDER BY id DESC LIMIT 1",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or_else(|_| "unknown".to_string());
                Ok((version, format))
            });

    match header {
        Ok((version, format)) => {
            let detail = format!(
                "{} · schema v{version} · {format} · {}",
                display_path(&path),
                if writable { "writable" } else { "not writable" }
            );
            result(
                "session",
                if writable {
                    CheckStatus::Ok
                } else {
                    CheckStatus::Warn
                },
                detail,
            )
        }
        Err(error) => warn(
            "session",
            format!("{} is unreadable: {error}", display_path(&path)),
        ),
    }
}

fn memory_check(options: &DoctorOptions) -> CheckResult {
    let found: Vec<String> = project_candidates(&options.workspace_root)
        .into_iter()
        .chain(user_candidates(
            &options.workspace_root,
            options.home.as_deref(),
        ))
        .filter(|path| path.exists())
        .map(|path| display_path(&path))
        .collect();

    if found.is_empty() {
        warn("memory", "no MEMORY.md or USER.md found")
    } else {
        ok("memory", found.join(", "))
    }
}

fn skills_check(workspace_root: &Path) -> CheckResult {
    let count = SkillManager::new(workspace_root).list_skills().len();
    ok("skills", format!("{count} installed"))
}

pub fn render(report: &DoctorReport) -> String {
    let mut text = format!("seavx doctor  v{} ({})\n", report.version, report.platform);

    for check in &report.checks {
        text.push_str(&format!(
            "\n{:<5} {:<11} {}",
            label(check.status),
            check.id,
            check.detail
        ));
    }

    let count = |status| {
        report
            .checks
            .iter()
            .filter(|check| check.status == status)
            .count()
    };
    text.push_str(&format!(
        "\n\nResult: {} ok, {} warnings, {} failures",
        count(CheckStatus::Ok),
        count(CheckStatus::Warn),
        count(CheckStatus::Fail)
    ));
    text
}

pub fn exit_code(report: &DoctorReport) -> i32 {
    i32::from(
        report
            .checks
            .iter()
            .any(|check| check.status == CheckStatus::Fail),
    )
}

fn label(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Ok => "OK",
        CheckStatus::Warn => "WARN",
        CheckStatus::Fail => "FAIL",
        CheckStatus::Skip => "SKIP",
    }
}

fn origin_label(origin: ConfigOrigin) -> &'static str {
    match origin {
        ConfigOrigin::Default => "default",
        ConfigOrigin::Workspace => "workspace",
        ConfigOrigin::Global => "global",
        ConfigOrigin::Env(_) => "env",
    }
}

fn redact_error(message: &str, base_url: &str) -> String {
    message.replace(base_url, &redact_url(base_url))
}

fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix("./").unwrap_or(&text).to_string()
}

fn ok(id: &'static str, detail: impl Into<String>) -> CheckResult {
    result(id, CheckStatus::Ok, detail)
}

fn warn(id: &'static str, detail: impl Into<String>) -> CheckResult {
    result(id, CheckStatus::Warn, detail)
}

fn fail(id: &'static str, detail: impl Into<String>) -> CheckResult {
    result(id, CheckStatus::Fail, detail)
}

fn result(id: &'static str, status: CheckStatus, detail: impl Into<String>) -> CheckResult {
    CheckResult {
        id,
        status,
        detail: detail.into(),
    }
}

struct HttpProbe;

#[async_trait::async_trait]
impl EndpointProbe for HttpProbe {
    async fn probe(&self, base_url: &str, api_key: Option<&str>) -> ProbeResult {
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(client) => client,
            Err(error) => return ProbeResult::Unreachable(error.to_string()),
        };

        let endpoint = format!("{}/models", base_url.trim_end_matches('/'));
        let mut request = client.get(&endpoint);
        if let Some(key) = api_key {
            request = request
                .header("Authorization", format!("Bearer {key}"))
                .header("x-api-key", key);
        }

        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => return ProbeResult::Unreachable(error.to_string()),
        };

        match response.status().as_u16() {
            401 | 403 => return ProbeResult::Unauthorized,
            404 | 405 => return ProbeResult::NotFound,
            code if !(200..300).contains(&code) => return ProbeResult::Status(code),
            _ => {}
        }

        match response.json::<serde_json::Value>().await {
            Ok(value) => {
                let models = value
                    .get("data")
                    .and_then(|entry| entry.as_array())
                    .or_else(|| value.get("models").and_then(|entry| entry.as_array()))
                    .map(|entries| entries.len())
                    .unwrap_or(0);
                ProbeResult::Ok { models }
            }
            Err(error) => ProbeResult::Unreachable(format!("invalid models response: {error}")),
        }
    }
}

#[cfg(test)]
#[path = "../tests/cli/doctor.rs"]
mod tests;
