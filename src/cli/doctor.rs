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
        secret_check(&config),
        context_check(&config),
        endpoint_check(&options, &config, &base_url, probe).await,
        cache_check(&options.workspace_root, &base_url),
        workspace_check(&options.workspace_root),
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

    let probe = directory.join(format!(".doctor-{}", std::process::id()));
    if let Err(error) = std::fs::write(&probe, b"ok") {
        let _ = std::fs::remove_file(&probe);
        return fail("workspace", format!("not writable: {error}"));
    }
    let _ = std::fs::remove_file(&probe);

    if existed {
        ok("workspace", "writable")
    } else {
        warn("workspace", "created `.seavx` during check")
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
