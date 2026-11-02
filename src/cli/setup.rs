//! `seavx setup providers`: prompt for endpoint, key, model and reasoning effort.

use super::prompt;

/// Steps as the user counts them: the endpoint question includes the key, so the
/// wizard is three steps, not four.
const ENDPOINT_STEP: &str = "endpoint (1/3)";
const MODEL_STEP: &str = "model (2/3)";
const EFFORT_STEP: &str = "effort (3/3)";

/// Print one step of the wizard. Never clears: `ClearType::All` fills the whole
/// console buffer on Windows, scrollback included, so a screen-clearing repaint
/// erased the steps the user had just answered. The step is named instead, and
/// `setup` is a one-time command whose output is worth keeping.
fn screen(config_path: &std::path::Path, step: &str, rows: &[(&str, String)]) {
    use std::io::{self, Write};
    println!("seavx setup providers · {step}");
    println!("config: {}\n", config_path.display());
    for (label, value) in rows {
        println!("{:<10} {}", format!("{label}:"), value);
    }
    if !rows.is_empty() {
        println!();
    }
    let _ = io::stdout().flush();
}

/// Persist the wizard's answers: the whole config, plus the key only when the
/// user typed one. `seavx setup` is one of the two surfaces allowed to repair an
/// unparsable file; the malformed bytes are kept beside it.
pub fn persist(
    path: &std::path::Path,
    config: &crate::agent::engine::AgentConfig,
    typed_key: Option<&str>,
) -> Result<Option<std::path::PathBuf>, String> {
    let preserved = crate::cli::config::write_agent_config_repairing(path, config)?;
    if let Some(key) = typed_key.filter(|key| !key.is_empty()) {
        crate::cli::config::set_api_key(path, key)?;
    }
    Ok(preserved)
}

/// The prompt default: what the target file says, else what is in effect.
fn seed(target: Option<&str>, effective: Option<&str>) -> String {
    target.or(effective).unwrap_or_default().to_string()
}

/// The file the wizard writes: this workspace's config when asked, the global
/// one otherwise.
pub fn target_path(
    workspace: bool,
    workspace_root: &std::path::Path,
    home: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, String> {
    if workspace {
        return Ok(crate::cli::config::workspace_config_path(workspace_root));
    }
    home.map(|home| home.join(".seavx").join("config.toml"))
        .ok_or_else(|| "home directory is not set, cannot locate config".to_string())
}

pub async fn providers(workspace: bool) -> anyhow::Result<()> {
    use std::io::{self, IsTerminal, Write};

    let config_path = target_path(
        workspace,
        std::path::Path::new("."),
        crate::home::dir().as_deref(),
    )
    .map_err(anyhow::Error::msg)?;

    // A malformed or missing file is replaced, matching AgentConfig::load's
    // fallback — seeded from the config in effect, so answers already given
    // (the key above all) are offered again instead of starting blank.
    let mut config: crate::agent::engine::AgentConfig = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| toml::from_str(&content).ok())
        .unwrap_or_else(|| crate::agent::engine::AgentConfig::load(std::path::Path::new(".")));
    // The effort menu reads what the TUI already cached; this never fetches.
    crate::agent::models::load_context_catalog(std::path::Path::new("."));

    let mask = |key: &str| {
        if key.is_empty() {
            "none".to_string()
        } else {
            "****".to_string()
        }
    };

    let model_mgr = crate::agent::models::ModelManager::new(&std::path::PathBuf::from("."));
    let mut available_models: Vec<String> = Vec::new();
    // Pre-filled from the config in effect, not only the target file: a key
    // exported in the environment or written to the workspace file is the key
    // `/models` is called with, so offering `none` for it is the 401 trap in
    // #30. Nothing from `effective` is written — `config` stays the source of
    // the file's contents.
    let effective = crate::agent::engine::AgentConfig::load(std::path::Path::new("."));
    let mut url_default = seed(config.base_url.as_deref(), effective.base_url.as_deref());
    let mut key_default = seed(config.api_key.as_deref(), effective.api_key.as_deref());

    // Ask the endpoint for its model list, so a wrong URL or key fails here instead of in the TUI.
    let (base_url, api_key) = 'endpoint: loop {
        screen(&config_path, ENDPOINT_STEP, &[]);
        println!(
            "Accepts an OpenAI- or Anthropic-compatible endpoint; /v1/ is added when missing.\n"
        );
        let url = prompt::line("Base URL", &url_default)?;
        // Checked before the key is asked, so a wrong scheme or a typo does not
        // cost a network round trip and a second prompt.
        let normalized = match crate::agent::engine::validate_base_url(&url) {
            Ok(normalized) => normalized,
            Err(error) => {
                println!("{error}; enter a URL like https://gateway.example/v1/\n");
                url_default = url;
                continue;
            }
        };

        // The key is asked until the gateway answers; a failure offers the same
        // "save anyway" exit as any other, so a keyless or 401 endpoint is never
        // a loop with no way out.
        loop {
            screen(
                &config_path,
                ENDPOINT_STEP,
                &[("base_url", normalized.clone())],
            );
            let key = prompt::secret("API key (optional)", &key_default)?;

            screen(
                &config_path,
                ENDPOINT_STEP,
                &[("base_url", normalized.clone()), ("api key", mask(&key))],
            );
            print!("checking /models ... ");
            io::stdout().flush()?;
            match model_mgr
                .refresh_remote_models(&normalized, Some(key.as_str()))
                .await
            {
                Ok(models) => {
                    println!("ok, {} models returned", models.len());
                    available_models = models;
                    break 'endpoint (normalized, key);
                }
                Err(e) => {
                    println!("failed: {e}");
                    let rejected = crate::agent::models::key_rejected(&e);
                    if rejected {
                        println!(
                            "the endpoint rejected that key; enter it again, or save this endpoint without a key\n"
                        );
                        // The URL answered, so the retry is about the key.
                        url_default = normalized.clone();
                    }
                    let keep = prompt::line("Save this endpoint anyway? [y/N]", "")?;
                    if keep.eq_ignore_ascii_case("y") {
                        break 'endpoint (normalized, key);
                    }
                    key_default.clear();
                    if rejected {
                        continue;
                    }
                    // A rejected attempt is forgotten, so the retry starts from blank fields.
                    url_default.clear();
                    continue 'endpoint;
                }
            }
        }
    };

    // One normalization, used by the live form and by the write below.
    let (stored_url, adapters) = crate::agent::engine::normalize_base_url(&base_url);
    let protocols = adapters
        .iter()
        .map(|a| format!("{a:?}"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut rows = vec![
        ("base_url", base_url.clone()),
        ("api key", mask(&api_key)),
        ("models", format!("{} available", available_models.len())),
        ("protocols", protocols.clone()),
    ];
    if available_models.is_empty() {
        rows.remove(2);
    }
    screen(&config_path, MODEL_STEP, &rows);
    let efforts = crate::agent::engine::EFFORT_LEVELS.join("|");
    let model = if available_models.is_empty() || !io::stdin().is_terminal() {
        let typed = prompt::line("Model", config.model.as_deref().unwrap_or_default())?;
        // The same gate `seavx model <name>` uses: a piped run cannot say yes,
        // and an interactive one is asked instead of warned.
        let host = crate::agent::models::ModelManager::domain_slug(&base_url);
        let interactive = io::stdin().is_terminal();
        match crate::cli::model::model_gate(interactive, &host, &available_models, &typed) {
            crate::cli::model::ModelGate::Writable => {}
            crate::cli::model::ModelGate::Ask(question) => {
                let answer = prompt::line(&question, "")?;
                let answer = answer.trim().to_ascii_lowercase();
                if answer != "y" && answer != "yes" {
                    anyhow::bail!("nothing written");
                }
            }
            crate::cli::model::ModelGate::Refused(reason) => {
                anyhow::bail!("{reason}; nothing written")
            }
        }
        typed
    } else {
        // The endpoint answered, so the choice is its list: a typed default here
        // is how a model the gateway does not serve gets saved.
        prompt::pick("Model", &available_models, config.model.as_deref())
            .ok_or_else(|| anyhow::anyhow!("cancelled; nothing written"))?
    };

    rows.push(("model", model.clone()));
    screen(&config_path, EFFORT_STEP, &rows);
    let effort = if io::stdin().is_terminal() {
        let levels = crate::agent::engine::effort_levels(Some(&model));
        let current = config.reasoning_effort.clone().unwrap_or_default();
        let current = if current.is_empty() {
            "none".to_string()
        } else {
            current
        };
        prompt::pick("Reasoning effort", &levels, Some(&current))
            .ok_or_else(|| anyhow::anyhow!("cancelled; nothing written"))?
    } else {
        prompt::line(
            &format!("Reasoning effort ({efforts})"),
            config.reasoning_effort.as_deref().unwrap_or(""),
        )?
    };

    config.base_url = Some(stored_url);
    config.model = Some(model);
    if !api_key.is_empty() {
        config.api_key = Some(api_key);
    }
    if !effort.is_empty() {
        if crate::agent::engine::parse_reasoning_effort(&effort).is_none() {
            anyhow::bail!("Unknown reasoning effort '{effort}', expected {efforts}");
        }
        config.reasoning_effort = Some(effort.to_lowercase());
    }

    let preserved =
        persist(&config_path, &config, config.api_key.as_deref()).map_err(anyhow::Error::msg)?;
    if let Some(preserved) = preserved {
        println!(
            "note: the previous config could not be parsed and was kept at {}",
            preserved.display()
        );
    }

    screen(
        &config_path,
        "saved",
        &[
            ("saved", "yes".to_string()),
            (
                "base_url",
                crate::cli::config::redact_url(config.base_url.as_deref().unwrap_or_default()),
            ),
            ("protocols", protocols),
            ("model", crate::cli::config::model_label(&config)),
            (
                "api key",
                mask(config.api_key.as_deref().unwrap_or_default()),
            ),
            (
                "effort",
                config
                    .reasoning_effort
                    .clone()
                    .unwrap_or_else(|| "none".to_string()),
            ),
        ],
    );
    if !workspace
        && std::path::PathBuf::from(".seavx")
            .join("config.toml")
            .exists()
    {
        println!("note: ./.seavx/config.toml overrides this file in the current workspace.");
    }

    // The budget and the effort menus read this cache, and setup is the first
    // command a new install runs: fetch it here so a headless `exec` has one.
    if let Err(error) =
        crate::agent::models::refresh_context_catalog(std::path::Path::new(".")).await
    {
        tracing::warn!(%error, "context catalog not refreshed");
    }

    Ok(())
}

#[cfg(test)]
#[path = "../tests/cli/setup.rs"]
mod tests;
